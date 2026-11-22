/**
 * Call coordinator (DESIGN.md §6): the glue between the daemon's call
 * sockets, the speech provider and a host consultation session per call.
 * Fail-closed by construction: without a configured speech provider nothing
 * connects, so the daemon simply sees a no-answer account.
 *
 * Inbound calls are answered only from the allowlist; conferences join only
 * on an explicit auto-join group whose inviter is an exact owner. Every
 * answered call gets a bridge (agent mode v1), a per-contact session key and
 * a duration watchdog; at the end the transcript is summarized into call
 * memory. No audio is recorded — only short-lived PCM buffers for the
 * current turn and the text transcript exist.
 */
import { randomUUID } from 'node:crypto';
import type { CallEvent } from '@agent-wechat/shared';
import type { ResolvedWeChatCallsConfig, ResolvedWeChatVoiceCallsConfig } from './config.ts';
import { resolveConferenceGroupConfig, type ResolvedWeChatConferenceGroupConfig } from './config.ts';
import { conferenceCallPolicy, directCallPolicy, isExactCallOwner } from './permissions.ts';
import { callSessionKey } from './session-keys.ts';
import type { CallToolPolicy } from './config.ts';
import { CallTranscript, formatCallTranscript } from './transcript.ts';
import { formatSummaryRecap, type CallSummaryStore } from './memory.ts';
import type { SpeechProvider } from './speech.ts';
import { AgentCallBridge } from './agent-bridge.ts';

export type CallResultView = { status: string; callId?: string; code?: string };

/** Structural subset of NativeCallsClient the coordinator touches. */
export type CallsClientLike = {
  connect(): Promise<unknown>;
  events(): AsyncIterable<CallEvent>;
  dial(requestId: string, contactId: string): Promise<CallResultView>;
  startConference(requestId: string, groupId: string, invitees: string[]): Promise<CallResultView>;
  invite(requestId: string, callId: string, contactIds: string[]): Promise<CallResultView>;
  joinConference(requestId: string, invitationId: string, invitationRevision: number): Promise<CallResultView>;
  answer(requestId: string, callId: string, expectedRevision: number): Promise<CallResultView>;
  end(type: 'call.reject' | 'call.cancel' | 'call.hangup', requestId: string, callId: string, expectedRevision: number): Promise<CallResultView>;
  clearAudio(requestId: string, callId: string): Promise<unknown>;
  attachMedia(callId: string): Promise<CallMediaClientLike>;
  close(): void;
};

export type CallMediaClientLike = {
  tracks(): { sendTrack: { generation: number } };
  frames(): AsyncIterable<{ payload: Uint8Array }>;
  sendPcm(samples: Int16Array, generation: number): void;
  close(): void;
};

export type CallCoordinatorDeps = {
  config: ResolvedWeChatCallsConfig;
  accountId: string;
  /** Harness owner allowlist (exact `agent-wechat:<id>` entries checked). */
  ownerAllowFrom: readonly string[];
  speech: SpeechProvider | undefined;
  createClient: () => CallsClientLike;
  /** Run one consultation turn in the host session for `sessionKey`. */
  runCallTurn: (sessionKey: string, text: string) => Promise<string>;
  summarizeCall?: (sessionKey: string, transcript: string) => Promise<string>;
  callMemory: CallSummaryStore;
  log?: (level: 'info' | 'warn', message: string) => void;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
};

type ActiveCall = {
  callId: string;
  groupId: string | undefined;
  contactId: string;
  direction: 'incoming' | 'outgoing';
  toolPolicy: CallToolPolicy;
  sessionKey: string;
  startedAt: number;
  transcript: CallTranscript;
  bridge: AgentCallBridge;
  recapSent: boolean;
  durationTimer: ReturnType<typeof setTimeout>;
};

const RETRY_INITIAL_MS = 1_000;
const RETRY_MAX_MS = 30_000;

export class CallCoordinator {
  private state: 'disabled' | 'connecting' | 'ready' | 'reconnecting' | 'stopped' = 'disabled';
  private client: CallsClientLike | undefined;
  private readonly calls = new Map<string, ActiveCall>();
  private running = false;
  private loop: Promise<void> | undefined;
  private chain: Promise<void>;
  private readonly now: () => number;
  private readonly sleep: (ms: number) => Promise<void>;

    private readonly deps: CallCoordinatorDeps;

  constructor(deps: CallCoordinatorDeps) {
    this.deps = deps;
    this.chain = Promise.resolve();
    this.now = deps.now ?? Date.now;
    this.sleep = deps.sleep ?? (ms => new Promise(resolve => setTimeout(resolve, ms)));
  }

  get available(): boolean {
    return this.deps.speech !== undefined && this.deps.config.voice !== undefined;
  }

  start(): void {
    if (this.running || this.state === 'stopped') return;
    if (!this.available) {
      this.state = 'disabled';
      this.deps.log?.('info', 'wechat calls: no speech provider configured; calls stay offline (fail-closed)');
      return;
    }
    this.running = true;
    this.state = 'connecting';
    this.loop = this.connectLoop();
  }

  private async connectLoop(): Promise<void> {
    let delay = RETRY_INITIAL_MS;
    while (this.running) {
      const client = this.deps.createClient();
      this.client = client;
      try {
        await client.connect();
        this.state = 'ready';
        delay = RETRY_INITIAL_MS;
        this.deps.log?.('info', 'wechat calls: control socket connected');
        await this.pumpEvents(client);
      } catch (error) {
        if (!this.running) return;
        this.deps.log?.('warn', `wechat calls: control socket lost (${error instanceof Error ? error.message : String(error)}); reconnecting`);
      }
      for (const callId of [...this.calls.keys()]) this.teardown(callId);
      this.client = undefined;
      if (!this.running) return;
      this.state = 'reconnecting';
      await this.sleep(delay);
      delay = Math.min(delay * 2, RETRY_MAX_MS);
    }
  }

  private async pumpEvents(client: CallsClientLike): Promise<void> {
    for await (const event of client.events()) {
      // Preserve event order; one bad event must not break the pump.
      this.chain = this.chain.then(() => this.handleEvent(client, event)).catch(error => {
        this.deps.log?.('warn', `wechat calls: event handling failed: ${error instanceof Error ? error.message : String(error)}`);
      });
    }
  }

  private async handleEvent(client: CallsClientLike, event: CallEvent): Promise<void> {
    if (event.type === 'call.offered') {
      const offer = event.offer;
      if (offer.kind !== 'direct') return; // conferences join via invitations
      const voice = this.requireVoice();
      const callerId = offer.callerId ?? '';
      if (voice.inboundPolicy !== 'allowlist' || !voice.inboundAllowFrom.includes(callerId)) {
        this.deps.log?.('info', `wechat calls: incoming call from ${callerId} is not on the inbound allowlist; ignoring`);
        return;
      }
      try {
        await client.answer(randomUUID(), offer.callId, offer.revision);
      } catch (error) {
        this.deps.log?.('warn', `wechat calls: answering failed: ${error instanceof Error ? error.message : String(error)}`);
        return;
      }
      await this.attachBridge(client, offer.callId, undefined, callerId, 'incoming', undefined, directCallPolicy(voice, callerId, this.deps.ownerAllowFrom));
      return;
    }
    if (event.type === 'conference.invitations') {
      const voice = this.requireVoice();
      const invitation = event.invitations?.[event.invitations.length - 1];
      if (!invitation) return;
      const group = resolveConferenceGroupConfig(voice, invitation.groupId);
      if (!group?.autoJoin || !isExactCallOwner(invitation.callerId, group.autoJoinAllowFrom)) return;
      try {
        await client.joinConference(randomUUID(), invitation.invitationId, invitation.revision);
      } catch (error) {
        this.deps.log?.('warn', `wechat calls: conference join failed: ${error instanceof Error ? error.message : String(error)}`);
      }
      return;
    }
    if (event.type === 'call.state') {
      const call = event.call;
      if (['ended', 'ending', 'uncertain'].includes(call.state)) {
        this.teardown(call.callId);
        return;
      }
      if (this.calls.has(call.callId) || !call.ownedByThisClient) return;
      const voice = this.requireVoice();
      const contactId = call.participants?.find(participant => participant.membership === 'joined' && participant.contactId)?.contactId
        ?? call.participants?.[0]?.contactId ?? '';
      const isConference = call.groupId !== undefined;
      let toolPolicy: CallToolPolicy;
      if (isConference) {
        const group = resolveConferenceGroupConfig(voice, call.groupId!)
          ?? ({ toolPolicy: voice.toolPolicy, autoJoin: false } satisfies ResolvedWeChatConferenceGroupConfig);
        toolPolicy = conferenceCallPolicy(voice, group, { groupId: call.groupId! }, this.deps.ownerAllowFrom).toolPolicy;
      } else {
        toolPolicy = directCallPolicy(voice, contactId, this.deps.ownerAllowFrom);
      }
      if (toolPolicy === 'none') {
        this.deps.log?.('info', 'wechat calls: call policy resolves to none; hanging up');
        await client.end('call.hangup', randomUUID(), call.callId, call.revision).catch(() => {});
        return;
      }
      await this.attachBridge(client, call.callId, call.groupId, contactId, call.direction === 'outgoing' ? 'outgoing' : 'incoming', isConference, toolPolicy);
    }
  }

  private requireVoice(): ResolvedWeChatVoiceCallsConfig {
    const voice = this.deps.config.voice;
    if (voice === undefined) throw new Error('calls are disabled');
    return voice;
  }

  private async attachBridge(
    client: CallsClientLike,
    callId: string,
    groupId: string | undefined,
    contactId: string,
    direction: 'incoming' | 'outgoing',
    isConference: boolean | undefined,
    toolPolicy: CallToolPolicy,
  ): Promise<void> {
    if (this.calls.has(callId)) return;
    const voice = this.requireVoice();
    const speech = this.deps.speech;
    if (!speech) throw new Error('no speech provider');
    const conference = groupId !== undefined;
    const sessionKey = callSessionKey(voice.agentId, voice.sessionScope, contactId, toolPolicy === 'owner', groupId !== undefined ? { accountId: this.deps.accountId, groupId } : undefined);
    const transcript = new CallTranscript();
    let media: CallMediaClientLike | undefined;
    const active: ActiveCall = {
      callId,
      groupId,
      contactId,
      direction,
      toolPolicy,
      sessionKey,
      startedAt: this.now(),
      transcript,
      // Assigned right below; the bridge closure references this shell.
      bridge: undefined as unknown as AgentCallBridge,
      recapSent: false,
      durationTimer: undefined as unknown as ReturnType<typeof setTimeout>,
    };
    active.bridge = new AgentCallBridge({
      provider: speech,
      sendPcm: (samples, generation) => {
        if (media) media.sendPcm(samples, generation);
      },
      onClearAudio: () => {
        void client.clearAudio(randomUUID(), callId).catch(() => {});
      },
      onUserUtterance: text => this.runTurn(active, text),
      onTranscript: (role, text, final) => transcript.add(role, text, final, false, this.now()),
      ...(this.deps.log !== undefined ? { log: this.deps.log } : {}),
      now: this.now,
    });
    media = await client.attachMedia(callId);
    const { sendTrack } = media.tracks();
    active.durationTimer = setTimeout(() => {
      this.deps.log?.('info', `wechat calls: call ${callId} hit the duration cap; hanging up`);
      void this.hangupCall(callId);
    }, voice.maxDurationSeconds * 1000);
    this.calls.set(callId, active);
    void this.pumpFrames(media, active.bridge, callId);
  }

  private async pumpFrames(media: CallMediaClientLike, bridge: AgentCallBridge, callId: string): Promise<void> {
    try {
      for await (const frame of media.frames()) {
        if (!this.calls.has(callId)) return;
        const samples = new Int16Array(frame.payload.buffer, frame.payload.byteOffset, Math.floor(frame.payload.byteLength / 2));
        bridge.acceptFrame(samples);
      }
    } catch {
      // Media socket ended (call over or client failed); teardown handles it.
    }
  }

  /** One agent turn; the contact's call-memory recap prefixes the first one. */
  private async runTurn(active: ActiveCall, text: string): Promise<string> {
    let input = text;
    if (!active.recapSent) {
      active.recapSent = true;
      const recap = formatSummaryRecap(await this.deps.callMemory.recent(
        this.deps.accountId,
        active.contactId,
        active.toolPolicy === 'none' ? undefined : active.toolPolicy,
      ));
      if (recap) input = `${recap}\n\nCaller: ${text}`;
    }
    return this.deps.runCallTurn(active.sessionKey, input);
  }

  private teardown(callId: string): void {
    const active = this.calls.get(callId);
    if (!active) return;
    this.calls.delete(callId);
    clearTimeout(active.durationTimer);
    active.bridge.close();
    const endedAt = this.now();
    const seconds = Math.round((endedAt - active.startedAt) / 1000);
    if (!this.deps.config.voice?.consultation.summary || !this.deps.summarizeCall) return;
    const text = formatCallTranscript(active.transcript.entries, active.startedAt);
    if (!text.trim()) return;
    void this.deps.summarizeCall(active.sessionKey, text)
      .then(summary => this.deps.callMemory.record(this.deps.accountId, active.contactId, {
        endedAt,
        seconds,
        direction: active.direction,
        summary,
      }, active.toolPolicy === 'none' ? undefined : active.toolPolicy))
      .catch(error => this.deps.log?.('warn', `wechat calls: summary failed: ${error instanceof Error ? error.message : String(error)}`));
  }

  // ---- tool actions ----

  status(): { state: string; available: boolean; calls: Array<{ callId: string; groupId?: string; contactId: string; direction: string; toolPolicy: CallToolPolicy }> } {
    return {
      state: this.state,
      available: this.available,
      calls: [...this.calls.values()].map(call => ({
        callId: call.callId,
        ...(call.groupId !== undefined ? { groupId: call.groupId } : {}),
        contactId: call.contactId,
        direction: call.direction,
        toolPolicy: call.toolPolicy,
      })),
    };
  }

  /** Tool-initiated outbound call. Requires the outbound allowlist. */
  async dial(contactId: string): Promise<CallResultView> {
    const client = this.requireClient();
    const voice = this.requireVoice();
    if (voice.outboundPolicy !== 'allowlist' || !voice.outboundAllowTo.includes(contactId)) {
      throw new Error(`calls to ${contactId} are not allowed by calls.voice.outboundAllowTo`);
    }
    return client.dial(`dial-${randomUUID()}`, contactId);
  }

  async conferenceStart(groupId: string, invitees: string[]): Promise<CallResultView> {
    return this.requireClient().startConference(`conf-${randomUUID()}`, groupId, invitees);
  }

  async invite(callId: string, contactIds: string[]): Promise<CallResultView> {
    return this.requireClient().invite(`invite-${randomUUID()}`, callId, contactIds);
  }

  async hangupCall(callId?: string): Promise<boolean> {
    const client = this.client;
    if (!client) return false;
    const target = callId ?? [...this.calls.keys()][0];
    if (target === undefined) return false;
    try {
      // expectedRevision 0 is usually stale; the daemon state advances anyway.
      await client.end('call.hangup', `hangup-${randomUUID()}`, target, 0);
    } catch (error) {
      this.deps.log?.('warn', `wechat calls: hangup request failed: ${error instanceof Error ? error.message : String(error)}`);
    }
    return true;
  }

  private requireClient(): CallsClientLike {
    if (this.state !== 'ready' || !this.client) throw new Error('the calls control socket is not connected');
    return this.client;
  }

  stop(): void {
    if (this.state === 'stopped') return;
    this.running = false;
    this.state = 'stopped';
    for (const callId of [...this.calls.keys()]) this.teardown(callId);
    this.client?.close();
    this.client = undefined;
  }
}
