import assert from 'node:assert/strict';
import { prepareOpenClawCallVoice } from '../../src/calls-voice.ts';
import { resolveWeChatCallsConfig } from '../../src/calls-config.ts';

/** Actual Gateway registries and agent dispatcher. Providers remain local. */
export function registerCallAgentFixture(api: any) {
  let transcription: any; const speech: string[] = [];
  api.registerRealtimeTranscriptionProvider({ id: 'fixture-stt', label: 'Fixture transcription', isConfigured: () => true,
    createSession(request: any) { transcription = request; let connected = false; return {
      connect: async () => { connected = true; }, isConnected: () => connected, sendAudio() {}, close: () => { connected = false; },
    }; },
  });
  api.registerSpeechProvider({ id: 'fixture-speech', label: 'Fixture speech', isConfigured: () => true,
    synthesize: async () => ({ audioBuffer: Buffer.alloc(2400, 1), outputFormat: 'pcm', fileExtension: '.pcm', voiceCompatible: false }),
    synthesizeTelephony: async ({ text }: any) => { speech.push(text); return { audioBuffer: Buffer.alloc(2400, 1), sampleRate: 24000, outputFormat: 'pcm' }; },
  });
  return async (cfg: any) => {
    const config = resolveWeChatCallsConfig(cfg.channels['agent-wechat'].calls)!.voice!;
    const prepared = await prepareOpenClawCallVoice(config, cfg, api.runtime);
    const proofs = [];
    for (const kind of ['direct', 'conference-owner', 'conference-read-only']) {
      const conference = kind !== 'direct'; const peer = conference ? 'synthetic-group@chatroom' : 'synthetic-human-owner';
      let frames = 0; const transcripts: string[] = []; const before = speech.length;
      const session = await prepared.open({ audio(_pcm, metadata) { assert.ok(metadata?.itemId); frames++; }, clear() {}, failed() { assert.fail('Agent fixture failed'); },
        transcript(role, text) { transcripts.push(`${role}:${text}`); } }, {
        senderId: peer, signal: new AbortController().signal, channelAccountId: 'default', nativeAccountId: 'a', connectedPeer: () => peer,
        ...(conference ? { conference: { groupId: peer, inviterId: kind === 'conference-owner' ? 'synthetic-human-owner' : 'synthetic-other-inviter' } } : {}),
      });
      const binding = session.binding!(); assert.equal(binding.owner, kind !== 'conference-read-only');
      transcription.onTranscript(`agent-${kind}-probe`);
      for (let i = 0; i < 600 && !frames; i++) await new Promise(resolve => setTimeout(resolve, 50));
      assert.ok(frames > 0, `Agent fixture produced no audio for ${kind}`);
      assert.equal(speech.length, before + 1); assert.ok(transcripts.some(text => text.startsWith('assistant:')));
      await session.close(); proofs.push({ kind, owner: binding.owner, audio: true, transcript: true });
    }
    return proofs;
  };
}
