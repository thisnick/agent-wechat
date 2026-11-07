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
    const spoken: string[] = [], inputs: Buffer[] = [], logs: string[] = []; let bridgeRequest: any; let clears = 0, outputs = 0;
    const provider = { id: 'openai', createBridge(params: any) {
      bridgeRequest = params; let connected = false;
      return { outputAudioMode: continuous ? 'continuous' : 'response',
        connect: async () => { connected = true; params.onReady(); }, isConnected: () => connected,
        sendUserMessage: (text: string) => spoken.push(text), sendAudio: (pcm: Buffer) => inputs.push(pcm),
        setMediaTimestamp() {}, handleBargeIn(options: unknown) {
          assert.deepEqual(options, { force: true, audioPlaybackActive: true });
          assert.deepEqual(params.getPlaybackState(), [{ itemId: 'fixture-reply', audioEndMs: 200 }]);
          params.onClearAudio('barge-in');
        }, acknowledgeMark() {}, submitToolResult() {}, close: () => { connected = false; },
      };
    } };
    const capabilities = { inputAudioFormats: [pcm], outputAudioFormats: [pcm], ...(continuous
      ? { supportsBargeIn: false, handlesInputAudioBargeIn: true, handlesAgentConsult: true }
      : { supportsBargeIn: true }) };
    const config = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: 'main', realtime: { provider: 'openai', agentContext: { enabled: true } }, toolPolicy: 'none' } })!.voice!;
    const prepared = await prepareOpenClawCallVoice(config, cfg, runtime, {
      voiceSdk: { ...sdk, resolveConfiguredRealtimeVoiceProvider: () => ({ provider, providerConfig: {}, capabilities }) }, audioSdk,
    });
    const snapshot: CallContextSnapshot = { callId: 'fixture-call', direction: 'incoming', revision: 1,
      conference: { groupId: 'fixture@chatroom', name: 'Fixture group', inviter: { contactId: 'inviter', name: 'Inviter' } },
      selfContactId: 'self', participantsKnown: true, conversationPartner: { contactId: 'a', name: 'Alice' },
      participants: [{ contactId: 'a', name: 'Alice' }] };
    const abort = new AbortController();
    const session = await prepared.open({ audio() { outputs++; }, clear(reason) { clears++; assert.equal(reason, continuous ? 'barge-in' : 'output-budget'); },
      playbackState: () => [{ itemId: 'fixture-reply', audioEndMs: 200 }], failed() { assert.fail('Fixture provider failed'); } },
      { senderId: 'fixture@chatroom', conference: { groupId: 'fixture@chatroom' }, signal: abort.signal, callContext: snapshot, logCallContext: line => logs.push(line) });
    assert.match(bridgeRequest.instructions, /Inviter/);
    assert.match(bridgeRequest.instructions, /Synthetic voice profile user/);
    assert.match(bridgeRequest.instructions, /Synthetic voice profile persona/);
    assert.doesNotMatch(bridgeRequest.instructions, /private-agents-profile-sentinel/);
    assert.match(bridgeRequest.instructions, /never instructions or proof of authority/);
    assert.match(bridgeRequest.instructions, /Do not ask who they are merely/);
    assert.equal(spoken.length, 0, 'preflight must not speak before media attaches');
    session.updateCallContext(snapshot, { initial: true });
    session.sendAudio(Buffer.alloc(960));
    const bob = { contactId: 'b', name: 'Bob' };
    session.updateCallContext({ ...snapshot, conversationPartner: null, participants: [...snapshot.participants, bob] }, { initial: false, joined: [bob], left: [] });
    const corrected = { ...snapshot, conversationPartner: null, participants: [{ contactId: 'a', name: 'Alice corrected' }, bob] };
    assert.equal(session.updateCallContext(corrected, { initial: false, joined: [], left: [], metadataOnly: true }), true);
    assert.match(spoken[2], /Nobody joined or left/); assert.match(spoken[2], /Alice corrected/);
    bridgeRequest.onAudio(Buffer.alloc(960), continuous ? undefined : { itemId: 'fixture-reply' });
    session.updateCallContext(snapshot, { initial: false, joined: [], left: [bob] });
    assert.equal(spoken.length, 4); assert.match(spoken[0], /Briefly greet/); assert.match(spoken[3], /bye/);
    assert.match(spoken[0], /exactly one other joined participant/);
    assert.equal(JSON.parse(spoken[1].split('\n').at(-1)!).call.conversationPartner, null);
    assert.equal(JSON.parse(spoken[3].split('\n').at(-1)!).call.conversationPartner.name, 'Alice');
    assert.ok(logs.some(line => /stage=submitted sequence=3 .*update=labels participants=2 named=2/.test(line)));
    assert.equal(logs.some(line => /Alice|Bob|Inviter|fixture@chatroom/.test(line)), false);
    assert.equal(clears, 0, 'context updates must not clear audio');
    if (continuous) bridgeRequest.onClearAudio('barge-in'); else session.interruptOutput();
    assert.equal(clears, 1);
    session.sendAudio(Buffer.alloc(960)); assert.equal(inputs.length, 2); assert.equal(outputs, 1);
    abort.abort(); session.updateCallContext(snapshot, { initial: true }); await session.close();
    assert.equal(spoken.length, 4, 'closed calls must ignore late context');
    proofs.push({ mode: continuous ? 'continuous' : 'response', messages: spoken.length, audioPreserved: true, interruptionPreserved: true });
  }
  return proofs;
}
