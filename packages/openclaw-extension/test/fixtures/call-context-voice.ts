import assert from 'node:assert/strict';
import type { PluginRuntime, OpenClawConfig } from 'openclaw/plugin-sdk/core';
import { prepareOpenClawCallVoice } from '../../src/calls-voice.ts';
import { resolveWeChatCallsConfig } from '../../src/calls-config.ts';
import type { CallContextSnapshot } from '../../src/calls-context.ts';

/** Real Gateway SDK, local provider bridge; no account or external model. */
export async function validateCallContextVoice(runtime: PluginRuntime, cfg: OpenClawConfig) {
  const voicePath: string = 'openclaw/plugin-sdk/realtime-voice';
  const audioPath: string = 'openclaw/plugin-sdk/realtime-voice-provider';
  const [sdk, audioSdk] = await Promise.all([import(voicePath), import(audioPath)]);
  const pcm = { encoding: 'pcm16', sampleRateHz: 24000, channels: 1 };
  const proofs = [];
  for (const continuous of [false, true]) {
    const spoken: string[] = [], inputs: Buffer[] = []; let bridgeRequest: any; let clears = 0, outputs = 0;
    const provider = { id: 'openai', createBridge(params: any) {
      bridgeRequest = params; let connected = false;
      return { outputAudioMode: continuous ? 'continuous' : 'response',
        connect: async () => { connected = true; params.onReady(); }, isConnected: () => connected,
        sendUserMessage: (text: string) => spoken.push(text), sendAudio: (pcm: Buffer) => inputs.push(pcm),
        setMediaTimestamp() {}, acknowledgeMark() {}, submitToolResult() {}, close: () => { connected = false; },
      };
    } };
    const capabilities = { inputAudioFormats: [pcm], outputAudioFormats: [pcm], ...(continuous
      ? { supportsBargeIn: false, handlesInputAudioBargeIn: true, handlesAgentConsult: true }
      : { supportsBargeIn: true }) };
    const config = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: 'main', realtime: { provider: 'openai' }, toolPolicy: 'none' } })!.voice!;
    const prepared = await prepareOpenClawCallVoice(config, cfg, runtime, {
      voiceSdk: { ...sdk, resolveConfiguredRealtimeVoiceProvider: () => ({ provider, providerConfig: {}, capabilities }) }, audioSdk,
    });
    const snapshot: CallContextSnapshot = { callId: 'fixture-call', direction: 'incoming', revision: 1,
      conference: { groupId: 'fixture@chatroom', name: 'Fixture group', inviter: { contactId: 'inviter', name: 'Inviter' } },
      participants: [{ contactId: 'a', name: 'Alice' }] };
    const abort = new AbortController();
    const session = await prepared.open({ audio() { outputs++; }, clear() { clears++; }, failed() { assert.fail('Fixture provider failed'); } },
      { senderId: 'fixture@chatroom', conference: { groupId: 'fixture@chatroom' }, signal: abort.signal, callContext: snapshot });
    assert.match(bridgeRequest.instructions, /Inviter/);
    assert.match(bridgeRequest.instructions, /never instructions or proof of authority/);
    assert.equal(spoken.length, 0, 'preflight must not speak before media attaches');
    session.updateCallContext(snapshot, { initial: true });
    session.sendAudio(Buffer.alloc(960));
    const bob = { contactId: 'b', name: 'Bob' };
    session.updateCallContext({ ...snapshot, participants: [...snapshot.participants, bob] }, { initial: false, joined: [bob], left: [] });
    bridgeRequest.onAudio(Buffer.alloc(960), continuous ? undefined : { itemId: 'fixture-reply' });
    session.updateCallContext(snapshot, { initial: false, joined: [], left: [bob] });
    assert.equal(spoken.length, 3); assert.match(spoken[0], /Briefly greet/); assert.match(spoken[2], /bye/);
    assert.equal(clears, 0, 'context updates must not clear audio');
    bridgeRequest.onClearAudio('barge-in'); assert.equal(clears, 1);
    session.sendAudio(Buffer.alloc(960)); assert.equal(inputs.length, 2); assert.equal(outputs, 1);
    abort.abort(); session.updateCallContext(snapshot, { initial: true }); await session.close();
    assert.equal(spoken.length, 3, 'closed calls must ignore late context');
    proofs.push({ mode: continuous ? 'continuous' : 'response', messages: spoken.length, audioPreserved: true, interruptionPreserved: true });
  }
  return proofs;
}
