import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  asAccountId,
  asChatJid,
  asSenderId,
  asServerId,
  type InboundMessage,
} from './contracts.ts';
import { describeInboundMedia, inboundFollowupText } from './inbound-media.ts';

function message(overrides?: Partial<InboundMessage>): InboundMessage {
  return {
    id: 'wechat-native:default:100:abc',
    ref: {
      accountId: asAccountId('default'),
      chatJid: asChatJid('wxid_alice'),
      serverId: asServerId('100'),
      senderId: asSenderId('wxid_alice'),
    },
    serverMessageId: '100',
    kind: 'text',
    text: 'hello',
    media: [],
    mentioned: false,
    createdAt: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

describe('describeInboundMedia', () => {
  it('renders no text for media-less messages', () => {
    assert.equal(describeInboundMedia([]), '');
  });

  it('never fabricates a voice transcript without a provider', () => {
    const text = describeInboundMedia([
      { assetId: 'asset-1', mimeType: 'audio/silk', kind: 'voice' },
    ]);
    assert.match(text, /transcription unavailable/);
    assert.doesNotMatch(text, /hello|你好/);
  });

  it('describes image and file assets with fetch guidance', () => {
    const text = describeInboundMedia([
      { assetId: 'asset-2', mimeType: 'image/jpeg', kind: 'image' },
      { assetId: 'asset-3', mimeType: 'application/pdf', kind: 'file' },
    ]);
    assert.match(text, /Image attachment: image\/jpeg, asset asset-2 — fetch with wechat_media/);
    assert.match(text, /File attachment: application\/pdf, asset asset-3 — fetch with wechat_media/);
  });

  it('describes stickers without download guidance', () => {
    const text = describeInboundMedia([{ assetId: 'asset-4', mimeType: 'image/gif', kind: 'sticker' }]);
    assert.match(text, /Sticker: asset asset-4/);
    assert.doesNotMatch(text, /wechat_media/);
  });

  it('appends media notes to the followup text', () => {
    const text = inboundFollowupText(
      message({
        kind: 'image',
        text: 'look',
        media: [{ assetId: 'asset-2', mimeType: 'image/png', kind: 'image' }],
      }),
    );
    assert.equal(text.startsWith('look\n[Image attachment: image/png'), true);
  });

  it('passes text-only messages through unchanged', () => {
    assert.equal(inboundFollowupText(message()), 'hello');
  });

  it('emits media notes alone when there is no caption', () => {
    const text = inboundFollowupText(
      message({ kind: 'voice', text: '', media: [{ assetId: 'a', mimeType: 'audio/silk', kind: 'voice' }] }),
    );
    assert.equal(text, '[Voice message: audio/silk — transcription unavailable: no speech provider configured]');
  });
});
